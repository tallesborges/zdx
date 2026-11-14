use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use zdx_engine::config::{Config, TelegramProfileConfig};
use zdx_engine::core::agent::ToolConfig;
use zdx_engine::orchestration::OrchestratorClient;

use crate::command_picker::CommandPickerMap;
use crate::delivery_tracker::ActiveDeliveries;
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
    /// Rebuilt whenever a config layer changes on disk (see
    /// [`BotContext::reload_config_if_changed`]).
    profile_configs: RwLock<HashMap<i64, Config>>,
    /// Profiles whose config names a model mode it does not define, keyed by
    /// `chat_id`, with the load error. Turns there are refused rather than run
    /// on the bot-level config, which may use another account.
    profile_errors: RwLock<HashMap<i64, String>>,
    /// Size + mtime of every config layer feeding `config`/`profile_configs`,
    /// so a turn can detect on-disk edits without re-parsing every time.
    config_watch: std::sync::Mutex<zdx_engine::config::ConfigWatch>,
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
    /// Client of the orchestrator service, which runs persistent-profile
    /// threads and owns their workers.
    orchestrator: OrchestratorClient,
    /// Thread id → (chat, message id) of the pinned header posted this
    /// process, so worker events can refresh an orchestrator's card in place.
    thread_headers: RwLock<HashMap<String, ThreadHeader>>,
    /// Reply deliveries running now; shared with the Mini App server.
    active_deliveries: ActiveDeliveries,
    /// `⬇ Send file` uploads running and recent stale-tap notices.
    send_files: crate::file_card::SendFileState,
    /// Thread ids of launcher orchestrator topics whose setup failed and that
    /// could not be deleted. Turns there are refused. Backs up the on-disk
    /// marker for when that write fails too.
    unsetup_topics: std::sync::Mutex<HashSet<String>>,
    /// Running agent-topic turns that take steers, and steer acknowledgements.
    steer: crate::steer::BotSteer,
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
    pub orchestrator: OrchestratorClient,
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
            orchestrator,
            cancel_map,
            queue_cancel_map,
            followup_map,
            retry_map,
            staging_map,
            command_picker_map,
            launcher_map,
        } = deps;
        let root = root.canonicalize().unwrap_or(root);
        let (profile_configs, profile_errors) = load_profile_configs(&config);
        let config_stamps = std::sync::Mutex::new(zdx_engine::config::ConfigWatch::new(
            &watched_config_layers(&root, &config),
        ));
        Self {
            client,
            config: RwLock::new(config),
            profile_configs: RwLock::new(profile_configs),
            profile_errors: RwLock::new(profile_errors),
            config_watch: config_stamps,
            allowlist_user_ids,
            allowlist_chat_ids,
            root,
            bot_instruction_layer,
            tool_config,
            orchestrator,
            exit_signal: Notify::new(),
            cancel_map,
            queue_cancel_map,
            followup_map,
            retry_map,
            staging_map,
            command_picker_map,
            launcher_map,
            thread_headers: RwLock::new(HashMap::new()),
            active_deliveries: ActiveDeliveries::default(),
            send_files: crate::file_card::SendFileState::default(),
            unsetup_topics: std::sync::Mutex::new(HashSet::new()),
            steer: crate::steer::BotSteer::default(),
        }
    }

    pub(crate) fn steer(&self) -> &crate::steer::BotSteer {
        &self.steer
    }

    pub(crate) fn unsetup_topics(&self) -> &std::sync::Mutex<HashSet<String>> {
        &self.unsetup_topics
    }

    pub(crate) fn active_deliveries(&self) -> &ActiveDeliveries {
        &self.active_deliveries
    }

    pub(crate) fn send_files(&self) -> &crate::file_card::SendFileState {
        &self.send_files
    }

    pub(crate) fn orchestrator(&self) -> &OrchestratorClient {
        &self.orchestrator
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

    /// Config a turn in `chat_id` may run with. Fails for a profile whose own
    /// config does not load because it names an undefined model mode: running
    /// the bot-level config there could use another provider or account.
    ///
    /// # Errors
    /// Returns the profile's config load error.
    pub(crate) fn checked_config_for_chat(&self, chat_id: i64) -> anyhow::Result<Config> {
        if let Some(err) = self
            .profile_errors
            .read()
            .expect("bot profile error lock poisoned")
            .get(&chat_id)
        {
            anyhow::bail!(
                "This chat's workspace config does not load, so turns here are refused \
                 instead of running another config's model: {err}"
            );
        }
        Ok(self.config_for_chat(chat_id))
    }

    /// Re-reads the config layers when any of them changed on disk, so edits
    /// made outside this process (monitor Config tab, an editor) apply to the
    /// next turn instead of waiting for a restart.
    ///
    /// Runtime `/model` changes survive this: they persist to the config layer
    /// for the chat's root before mutating the in-memory config, so the reload
    /// reads the same value back.
    ///
    /// Startup-derived state (allowlists, Telegram settings, tool registry) is
    /// not affected by a reload and still needs a restart.
    pub(crate) fn reload_config_if_changed(&self) {
        let mut watch = self
            .config_watch
            .lock()
            .expect("bot config watch lock poisoned");
        if !watch.changed(&watched_config_layers(&self.root, &self.config())) {
            return;
        }

        match Config::load_layered(&zdx_engine::config::paths::config_layer_paths_for(
            &self.root,
        )) {
            Ok(config) => {
                let (profile_configs, profile_errors) = load_profile_configs(&config);
                // Restamp from the reloaded config: the same edit may have
                // added, removed, or repointed a profile.
                watch.restamp(&watched_config_layers(&self.root, &config));
                tracing::info!(model = %config.model, "Reloaded config after on-disk change");
                *self
                    .profile_configs
                    .write()
                    .expect("bot profile config lock poisoned") = profile_configs;
                *self
                    .profile_errors
                    .write()
                    .expect("bot profile error lock poisoned") = profile_errors;
                *self.config.write().expect("bot config lock poisoned") = config;
            }
            Err(err) => {
                // Keep serving the last good config; restamping stops a broken
                // file from being re-parsed on every turn (a fix changes the
                // stamp again).
                watch.restamp(&watched_config_layers(&self.root, &self.config()));
                tracing::warn!(err = %format!("{err:#}"), "Failed to reload config; keeping the previous one");
            }
        }
    }

    /// Persists a runtime model change for `chat_id`.
    ///
    /// The selection is written to the chat root's workspace overlay (the
    /// profile cwd, else the bot's own cwd) and applied to that chat's config
    /// only, so bound projects keep independent defaults.
    pub(crate) fn set_chat_model(&self, chat_id: i64, model_id: &str) -> anyhow::Result<()> {
        let mut resolved = self.checked_config_for_chat(chat_id)?;
        resolved.apply_model_selection(model_id)?;
        Config::save_model_for_cwd(
            &self.root_for_chat(chat_id).root,
            &resolved.model_selection(),
        )?;
        self.update_config_for_chat(chat_id, |cfg| cfg.apply_model_spec(model_id));
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

    /// Remembers where a thread's pinned header lives and what it last showed
    /// (process-lifetime).
    pub(crate) fn record_thread_header(&self, thread_id: &str, header: ThreadHeader) {
        self.thread_headers
            .write()
            .expect("thread header lock poisoned")
            .insert(thread_id.to_string(), header);
    }

    /// A thread's pinned header posted this process.
    pub(crate) fn thread_header(&self, thread_id: &str) -> Option<ThreadHeader> {
        self.thread_headers
            .read()
            .expect("thread header lock poisoned")
            .get(thread_id)
            .cloned()
    }
}

#[cfg(test)]
impl BotContext {
    /// Points the context at a test orchestrator service.
    pub(crate) fn with_orchestrator(mut self, orchestrator: OrchestratorClient) -> Self {
        self.orchestrator = orchestrator;
        self
    }

    /// A context with empty per-process maps, for tests that need a whole
    /// context (for example with a client pointed at a fake Bot API).
    pub(crate) fn for_test(client: TelegramClient, config: Config, root: PathBuf) -> Self {
        Self::for_test_allowing(client, config, root, HashSet::new(), HashSet::new())
    }

    /// [`Self::for_test`] with allowlists, for tests that go through the
    /// queue's sender gate.
    pub(crate) fn for_test_allowing(
        client: TelegramClient,
        config: Config,
        root: PathBuf,
        allowlist_user_ids: HashSet<i64>,
        allowlist_chat_ids: HashSet<i64>,
    ) -> Self {
        Self::new(
            client,
            config,
            BotContextDeps {
                allowlist_user_ids,
                allowlist_chat_ids,
                root,
                bot_instruction_layer: None,
                tool_config: ToolConfig::default(),
                // Points at a socket nothing listens on: tests see no
                // service, as when it is stopped.
                orchestrator: OrchestratorClient::at(
                    std::env::temp_dir().join(format!("zdx-test-{}.sock", uuid::Uuid::new_v4())),
                ),
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
}

/// A thread's pinned header message and the content it last showed, so an
/// unchanged re-render skips the edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadHeader {
    pub(crate) chat_id: i64,
    pub(crate) message_id: i64,
    /// Rendered text plus serialized keyboard last accepted by Telegram.
    pub(crate) rendered: String,
}

fn profile_root_path(profile: &TelegramProfileConfig) -> PathBuf {
    let root = profile.cwd_path();
    root.canonicalize().unwrap_or(root)
}

/// Every config file that feeds the bot-level config or any profile config, so
/// an edit to any of them triggers one reload.
fn watched_config_layers(root: &Path, base: &Config) -> Vec<PathBuf> {
    zdx_engine::telegram::watched_config_layers(root, base)
}

/// Loads one layered [`Config`] per Telegram profile, anchored at the profile's
/// cwd so a workspace `.zdx/config.toml` applies to chats bound to it.
///
/// Runs at startup and again whenever [`BotContext::reload_config_if_changed`]
/// sees a layer change on disk. A profile whose layers fail to load is skipped
/// and falls back to the bot-level config, so one broken workspace file cannot
/// take the whole bot down. A profile that fails because its config names an
/// undefined model mode is returned in the error map instead: that chat fails
/// closed, since the fallback could run another account's model.
fn load_profile_configs(base: &Config) -> (HashMap<i64, Config>, HashMap<i64, String>) {
    let mut configs = HashMap::new();
    let mut errors = HashMap::new();

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
            Err(err) if zdx_engine::config::UnknownModeError::is_in(&err) => {
                tracing::warn!(
                    profile = %name,
                    root = %root.display(),
                    err = %format!("{err:#}"),
                    "Profile config names an undefined model mode; refusing turns in its chat",
                );
                errors.insert(profile.chat_id, format!("{err:#}"));
            }
            Err(err) => {
                tracing::warn!(
                    profile = %name,
                    root = %root.display(),
                    err = %format!("{err:#}"),
                    "Failed to load profile config layers; falling back to bot config",
                );
            }
        }
    }

    (configs, errors)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use zdx_engine::config::{TelegramConfig, ThinkingLevel};

    use super::*;

    #[test]
    fn test_root_for_chat_uses_matching_profile_cwd() {
        // Profile configs layer over the global config: keep it off the
        // developer's real `ZDX_HOME`.
        let _home = zdx_engine::test_support::temp_zdx_home();
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
                        worker_root: None,
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
    fn orchestrator_flag_is_per_chat_opt_in() {
        // Profile configs layer over the global config: keep it off the
        // developer's real `ZDX_HOME`.
        let _home = zdx_engine::test_support::temp_zdx_home();
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
                            worker_root: None,
                        },
                    ),
                    (
                        "project".to_string(),
                        TelegramProfileConfig {
                            chat_id: -100_222,
                            cwd: root.display().to_string(),
                            orchestrator: false,
                            worker_root: None,
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
        // Profile configs layer over the global config: keep it off the
        // developer's real `ZDX_HOME`.
        let _home = zdx_engine::test_support::temp_zdx_home();
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
                        worker_root: None,
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
        // Profile configs layer over the global config: keep it off the
        // developer's real `ZDX_HOME`.
        let _home = zdx_engine::test_support::temp_zdx_home();
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
                        worker_root: None,
                    },
                )]),
                ..Default::default()
            },
            model: "sentinel:global-model".to_string(),
            ..Default::default()
        };
        let context = test_context(config, fallback_root);

        context.set_chat_model(-100_123, "sentinel:picked").unwrap();

        assert_eq!(
            context.config_for_chat(-100_123).model,
            "sentinel:picked@off"
        );
        assert_eq!(
            context.config_for_chat(-100_999).model,
            "sentinel:global-model"
        );
        assert_eq!(context.config().model, "sentinel:global-model");

        let overlay = fs::read_to_string(profile_root.join(".zdx").join("config.toml")).unwrap();
        assert_eq!(overlay.trim(), "model = \"sentinel:picked@off\"");
    }

    /// A config edit landing on disk after startup (monitor Config tab, an
    /// editor) applies on the next turn, without restarting the bot.
    #[test]
    fn test_reload_applies_on_disk_config_edits() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let fallback_root = unique_temp_dir("reload-fallback");
        let profile_root = unique_temp_dir("reload-profile");
        fs::create_dir_all(&fallback_root).unwrap();
        fs::create_dir_all(profile_root.join(".zdx")).unwrap();

        let overlay = profile_root.join(".zdx").join("config.toml");
        fs::write(&overlay, "model = \"sentinel:before\"\n").unwrap();
        let global = home.path().join("config.toml");
        fs::write(
            &global,
            format!(
                "model = \"sentinel:global\"\n\n[telegram.profiles.zdx]\nchat_id = -100123\ncwd = \"{}\"\n",
                profile_root.display()
            ),
        )
        .unwrap();

        let context = test_context(
            Config::load_layered(std::slice::from_ref(&global)).unwrap(),
            fallback_root,
        );
        assert_eq!(context.config_for_chat(-100_123).model, "sentinel:before");

        fs::write(
            &overlay,
            "model = \"sentinel:after-the-edit\"\n\n[subagents.overrides.oracle]\nmodel = \"sentinel:oracle-override\"\n",
        )
        .unwrap();
        context.reload_config_if_changed();

        let reloaded = context.config_for_chat(-100_123);
        assert_eq!(reloaded.model, "sentinel:after-the-edit");
        assert_eq!(
            reloaded
                .subagents
                .overrides
                .get("oracle")
                .and_then(|over| over.model.as_deref()),
            Some("sentinel:oracle-override")
        );
    }

    /// `/model` persists to the chat root's overlay before mutating the
    /// in-memory config, so a later reload must read the same value back
    /// instead of reverting the runtime choice.
    #[test]
    fn test_runtime_model_spec_change_survives_reload() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let fallback_root = unique_temp_dir("reload-set-fallback");
        let profile_root = unique_temp_dir("reload-set-profile");
        fs::create_dir_all(&fallback_root).unwrap();
        fs::create_dir_all(profile_root.join(".zdx")).unwrap();

        let global = home.path().join("config.toml");
        fs::write(
            &global,
            format!(
                "model = \"sentinel:global\"\n\n[telegram.profiles.zdx]\nchat_id = -100123\ncwd = \"{}\"\n",
                profile_root.display()
            ),
        )
        .unwrap();

        let context = test_context(
            Config::load_layered(std::slice::from_ref(&global)).unwrap(),
            fallback_root,
        );
        context
            .set_chat_model(-100_123, "sentinel:picked@high")
            .unwrap();

        context.reload_config_if_changed();

        // The level is persisted as the model's `@<level>` suffix.
        assert_eq!(
            context.config_for_chat(-100_123).model,
            "sentinel:picked@high"
        );
        assert_eq!(
            context.config_for_chat(-100_123).thinking_level,
            ThinkingLevel::High
        );
    }

    /// A profile whose layered config names a mode it does not define fails
    /// closed: its turns are refused instead of running the bot-level config,
    /// which may use another account. A fix applies on reload.
    #[test]
    fn profile_with_an_undefined_mode_fails_closed() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let fallback_root = unique_temp_dir("mode-fallback");
        let profile_root = unique_temp_dir("mode-profile");
        fs::create_dir_all(&fallback_root).unwrap();
        fs::create_dir_all(profile_root.join(".zdx")).unwrap();

        // The overlay replaces the mode list without `smart`, which the
        // global default names.
        let overlay = profile_root.join(".zdx").join("config.toml");
        fs::write(
            &overlay,
            "[[model_modes]]\nname = \"fast\"\nprimary = \"work:small@low\"\n",
        )
        .unwrap();
        let global = home.path().join("config.toml");
        fs::write(
            &global,
            format!(
                "model = \"mode:smart\"\n[[model_modes]]\nname = \"smart\"\nprimary = \"personal:big@high\"\n\n[telegram.profiles.work]\nchat_id = -100123\ncwd = \"{}\"\n",
                profile_root.display()
            ),
        )
        .unwrap();

        let context = test_context(
            Config::load_layered(std::slice::from_ref(&global)).unwrap(),
            fallback_root,
        );
        let err = context.checked_config_for_chat(-100_123).unwrap_err();
        assert!(format!("{err:#}").contains("`mode:smart`"), "{err:#}");
        assert!(
            context.set_chat_model(-100_123, "work:other@low").is_err(),
            "no write may resolve against the bot-level config either"
        );
        assert!(context.checked_config_for_chat(-100_999).is_ok());

        fs::write(
            &overlay,
            "model = \"mode:fast\"\n[[model_modes]]\nname = \"fast\"\nprimary = \"work:small@low\"\n",
        )
        .unwrap();
        context.reload_config_if_changed();
        let fixed = context.checked_config_for_chat(-100_123).unwrap();
        assert_eq!(fixed.model, "work:small@low");
        assert_eq!(fixed.model_selection(), "mode:fast");
    }

    fn test_context(config: Config, root: PathBuf) -> BotContext {
        BotContext::for_test(TelegramClient::new("token".to_string()), config, root)
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("zdx-bot-profile-{label}-{}", uuid::Uuid::new_v4()))
    }
}
