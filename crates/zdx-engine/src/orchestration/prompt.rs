//! Turn preparation for persistent-profile threads: the profile's prompt,
//! tool list, and model, plus the instruction layer of the surface the turn
//! answers on.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::protocol::Route;
use crate::config::Config;
use crate::core::context::{PromptContextInclusion, RuntimeContext};
use crate::subagents::{self, ORCHESTRATOR_SUBAGENT_NAME, SubagentDefinition};

/// A persistent-profile turn ready to run.
pub struct PreparedTurn {
    /// Config with the profile's and the thread's model choices applied.
    pub config: Config,
    pub system_prompt: String,
    /// Advisory runtime-context snapshot for the next user message.
    pub runtime_context: Option<RuntimeContext>,
    /// Exact tool allowlist declared by the profile.
    pub tools: Vec<String>,
    /// Subagents `invoke_subagent` may reach; `None` is unrestricted.
    pub allowed_subagents: Option<Vec<String>>,
}

/// Model config a persistent-profile turn runs with: the base config, then
/// the profile's model and `[subagents.overrides.<profile>]`, then the
/// thread's own override (the bot's `/model`, or `--model` at creation).
#[must_use]
pub fn turn_config(
    mut config: Config,
    definition: &SubagentDefinition,
    model_override: Option<&str>,
    thinking_override: Option<crate::config::ThinkingLevel>,
) -> Config {
    subagents::apply_persistent_profile_model(&mut config, definition);
    config.apply_thread_model_override(model_override, thinking_override);
    config
}

/// Builds the system prompt and tool allowlist for one turn of `definition`.
///
/// The profile prompt sees the full discovered skills catalog. A Telegram
/// route adds the Telegram instruction layer and the workspace list; any
/// other turn gets the non-interactive exec layer. The personal overlay
/// (`$ZDX_HOME/orchestrator.md`) is appended to the built-in orchestrator
/// only.
///
/// # Errors
/// Returns an error if the prompt cannot be rendered.
pub fn prepare_turn(
    config: &Config,
    root: &Path,
    definition: &SubagentDefinition,
    route: Option<Route>,
) -> Result<PreparedTurn> {
    // The workspace catalog is observational data: it goes into the
    // runtime-context snapshot, not the system prompt, so it never churns the
    // stable prefix.
    let workspaces = route.and_then(|route| telegram_workspaces_block(config, Some(route.chat)));
    let (mut prompt, runtime_context) =
        subagents::render_prompt_with_discovered_skills_and_context(
            config,
            root,
            definition,
            &config.model,
            PromptContextInclusion {
                project_context: true,
                memory_index: true,
                skills: true,
            },
            workspaces.as_deref(),
        )
        .with_context(|| format!("render profile '{}'", definition.name))?;
    if definition.name == ORCHESTRATOR_SUBAGENT_NAME {
        let overlay_path = crate::config::paths::zdx_home().join("orchestrator.md");
        if let Some(overlay) = load_orchestrator_overlay(&overlay_path) {
            prompt = format!("{prompt}\n\n# Personal Orchestrator Rules\n\n{overlay}");
        }
    }
    let layer = if route.is_some() {
        crate::prompts::TELEGRAM_INSTRUCTION_LAYER
    } else {
        crate::prompts::EXEC_INSTRUCTION_LAYER
    };
    let layer = layer.trim();
    if !layer.is_empty() {
        prompt = format!("{prompt}\n\n{layer}");
    }

    Ok(PreparedTurn {
        config: config.clone(),
        system_prompt: prompt,
        runtime_context,
        tools: definition.tools.clone().unwrap_or_default(),
        allowed_subagents: definition.allowed_subagents.clone(),
    })
}

/// Telegram **workspace** groups bound to the bot, so the orchestrator can pick
/// worker roots deliberately. Each workspace also lists the project skills its
/// workers discover there, since the orchestrator's own skill catalog only
/// covers its home root.
///
/// Orchestrator homes are not listed: they are not worker destinations and
/// never own skills. `chat_id` is the current chat, used to state its
/// configured default worker root. `None` when no workspace profiles are
/// configured.
#[must_use]
pub fn telegram_workspaces_block(config: &Config, chat_id: Option<i64>) -> Option<String> {
    let workspaces: Vec<_> = config
        .telegram
        .profiles
        .iter()
        .filter(|(_, profile)| profile.is_routable_workspace())
        .collect();
    if workspaces.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let skills = workspace_skills(config);

    let mut block = String::from(
        "# Telegram Workspaces\n\nWorkspace groups bound to this bot (profile — root), the project roots workers usually run in.\n\nWorkers in a root automatically discover that project's skills (listed under it); you do not have them yourself. Use them to know what a workspace can do and to point a worker at the right one by name when the task matches.\n\n",
    );
    for (name, profile) in workspaces {
        let root = shorten_home(&profile.cwd_path().display().to_string(), &home);
        let _ = writeln!(block, "- {name} (chat {}) — `{root}`", profile.chat_id);
        for (skill_name, description) in skills.get(name).into_iter().flatten() {
            let _ = writeln!(block, "  - skill `{skill_name}`: {description}");
        }
    }

    // The default applies when `Create_Thread` is called without a root; state
    // it so the choice is deliberate rather than implied by the listing.
    if let Some(worker_root) = chat_id
        .and_then(|chat_id| config.telegram_profile_for_chat(chat_id))
        .filter(|(_, profile)| !profile.is_routable_workspace())
        .map(|(_, profile)| profile.worker_root_path())
    {
        let root = shorten_home(&worker_root.display().to_string(), &home);
        let _ = write!(
            block,
            "\nYour default worker root is `{root}`. Omit `root` in `Create_Thread` to use it; pass an explicit `root` for work that belongs to a specific project.\n"
        );
    }
    Some(block)
}

/// Longest skill description shown per workspace skill line.
const WORKSPACE_SKILL_DESCRIPTION_CHARS: usize = 140;

/// Project-level skills each Telegram workspace's workers will discover, keyed
/// by profile name. Only project sources are scanned (user/global skills are
/// already in the orchestrator's own catalog). A skill reachable from several
/// nested workspaces is attributed once, to the deepest root that contains it,
/// and listed under every workspace bound to that root.
///
/// Orchestrator homes are skipped: they are not rendered, so letting one win
/// attribution would drop the skills from the prompt entirely.
fn workspace_skills(config: &Config) -> HashMap<String, Vec<(String, String)>> {
    use crate::config::SkillSourceToggles;
    use crate::skills::{LoadSkillsOptions, SkillSource, load_skills};

    // skill file → (owning root, its depth, skill name, description)
    let mut owner: HashMap<PathBuf, (PathBuf, usize, String, String)> = HashMap::new();
    let mut profiles_by_root: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for (name, profile) in &config.telegram.profiles {
        if !profile.is_routable_workspace() {
            continue;
        }
        let root = profile.cwd_path();
        let root = root.canonicalize().unwrap_or(root);
        let sharing = profiles_by_root.entry(root.clone()).or_default();
        sharing.push(name.clone());
        if sharing.len() > 1 {
            // Same root, same skills: already scanned for an earlier profile.
            continue;
        }
        let depth = root.components().count();
        let options = LoadSkillsOptions {
            sources: SkillSourceToggles {
                zdx_project: true,
                claude_project: true,
                agents_project: true,
                ..SkillSourceToggles::default()
            },
            ..LoadSkillsOptions::new(root.clone())
        };
        // Bundled skills ignore the toggles; keep project sources only.
        let project_skills = load_skills(&options).skills.into_iter().filter(|skill| {
            matches!(
                skill.source,
                SkillSource::ZdxProject | SkillSource::ClaudeProject | SkillSource::AgentsProject
            )
        });
        for skill in project_skills {
            let contains = skill.base_dir.starts_with(&root);
            // Prefer the deepest root that actually contains the skill; a
            // skill inherited from an ancestor without a profile goes to
            // whichever workspace saw it first.
            let rank = if contains { depth } else { 0 };
            let replace = owner
                .get(&skill.file_path)
                .is_none_or(|(_, existing, _, _)| rank > *existing);
            if replace {
                owner.insert(
                    skill.file_path.clone(),
                    (
                        root.clone(),
                        rank,
                        skill.name.clone(),
                        truncate_description(&skill.description, WORKSPACE_SKILL_DESCRIPTION_CHARS),
                    ),
                );
            }
        }
    }

    let mut by_profile: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (root, _, skill_name, description) in owner.into_values() {
        for profile in profiles_by_root.get(&root).into_iter().flatten() {
            by_profile
                .entry(profile.clone())
                .or_default()
                .push((skill_name.clone(), description.clone()));
        }
    }
    for skills in by_profile.values_mut() {
        skills.sort();
    }
    by_profile
}

fn truncate_description(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= max_chars {
        return first_line.to_string();
    }
    let head: String = first_line.chars().take(max_chars).collect();
    format!("{}…", head.trim_end())
}

/// Loads the user's personal orchestrator overlay (`$ZDX_HOME/orchestrator.md`):
/// free-form manager rules appended only to built-in orchestrator turns,
/// editable without rebuilding. A missing file is a no-op; other read failures
/// are logged and skipped so a broken overlay never blocks the home base.
fn load_orchestrator_overlay(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "Failed to read orchestrator overlay");
            None
        }
    }
}

fn shorten_home(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::{SkillSourceToggles, TelegramConfig, TelegramProfileConfig};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn quiet_config() -> Config {
        let mut config = Config::default();
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };
        config
    }

    #[test]
    fn orchestrator_turn_renders_project_context_and_pins_tools() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = temp_dir();
        std::fs::write(dir.path().join("AGENTS.md"), "Orchestrator project note").unwrap();
        let definition = subagents::load_persistent_profile(dir.path(), "orchestrator").unwrap();

        let telegram = prepare_turn(
            &quiet_config(),
            dir.path(),
            &definition,
            Some(Route {
                chat: -100,
                topic: Some(7),
            }),
        )
        .unwrap();
        // SPEC §18: profile prompt composes project context + surface layer.
        assert!(telegram.system_prompt.contains("ZDX Orchestrator"));
        assert!(telegram.system_prompt.contains("Orchestrator project note"));
        assert!(
            telegram
                .system_prompt
                .contains(crate::prompts::TELEGRAM_INSTRUCTION_LAYER.trim())
        );

        // Exact profile tool surface, no default-toolset fallback.
        assert!(telegram.tools.contains(&"create_thread".to_string()));
        assert!(!telegram.tools.contains(&"write".to_string()));
        assert!(!telegram.tools.contains(&"bash".to_string()));
        assert!(!telegram.tools.contains(&"invoke_subagent".to_string()));
        // SPEC §18: the home base delegates to visible workers, not subagents.
        assert_eq!(telegram.allowed_subagents, None);

        // A turn with no Telegram route answers a script, not a chat.
        let cli = prepare_turn(&quiet_config(), dir.path(), &definition, None).unwrap();
        assert!(
            cli.system_prompt
                .contains(crate::prompts::EXEC_INSTRUCTION_LAYER.trim())
        );
        assert!(
            !cli.system_prompt
                .contains(crate::prompts::TELEGRAM_INSTRUCTION_LAYER.trim())
        );
    }

    #[test]
    fn a_custom_profile_supplies_prompt_tools_and_model() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = temp_dir();
        let profiles = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(
            profiles.join("triage.md"),
            "---\nname: triage\ndescription: Example triage manager.\nmodel: sentinel:triage-model\ntools:\n  - read\n  - create_thread\n---\nYou are the example triage manager.\n",
        )
        .unwrap();

        let definition = subagents::load_persistent_profile(dir.path(), "triage").unwrap();
        let prepared = prepare_turn(&quiet_config(), dir.path(), &definition, None).unwrap();
        assert!(
            prepared
                .system_prompt
                .contains("You are the example triage manager.")
        );
        assert_eq!(prepared.tools, vec!["read", "create_thread"]);

        let config = turn_config(Config::default(), &definition, None, None);
        assert!(config.model.starts_with("sentinel:triage-model"));
        // The thread's own override wins.
        let config = turn_config(
            Config::default(),
            &definition,
            Some("sentinel:picked"),
            None,
        );
        assert!(config.model.starts_with("sentinel:picked"));
    }

    #[test]
    fn unknown_or_toolless_profiles_fail() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = temp_dir();
        assert!(subagents::load_persistent_profile(dir.path(), "mystery").is_err());

        let profiles = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(
            profiles.join("bare.md"),
            "---\nname: bare\ndescription: No tools.\n---\nbody\n",
        )
        .unwrap();
        let err = subagents::load_persistent_profile(dir.path(), "bare")
            .expect_err("a profile without tools must fail");
        assert!(err.to_string().contains("declares no tools"));
    }

    #[test]
    fn telegram_workspaces_block_lists_profiles_with_routing_rule() {
        // Rendering skills materializes bundled skills into $ZDX_HOME.
        let _home = crate::test_support::temp_zdx_home();

        assert!(telegram_workspaces_block(&Config::default(), None).is_none());

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "dub".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1001,
                            cwd: "/tmp/work/dub".to_string(),
                            orchestrator: false,
                            worker_root: None,
                        },
                    ),
                    (
                        "zdx".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1002,
                            cwd: "/tmp/personal/zdx".to_string(),
                            orchestrator: true,
                            worker_root: None,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let block = telegram_workspaces_block(&config, None).unwrap();
        assert!(block.starts_with("# Telegram Workspaces"));
        assert!(block.contains("the project roots workers usually run in"));
        assert!(block.contains("- dub (chat -1001) — `/tmp/work/dub`"));
        // Orchestrator homes are not worker destinations, so they are not
        // offered as workspaces at all.
        assert!(!block.contains("zdx (chat -1002)"));

        // Rendered for the orchestrator's own chat, the block states the
        // default root it gets when `Create_Thread` omits one.
        let block = telegram_workspaces_block(&config, Some(-1002)).unwrap();
        assert!(block.contains("Your default worker root is `/tmp/personal/zdx`"));
        // A workspace chat has no default of its own.
        assert!(
            !telegram_workspaces_block(&config, Some(-1001))
                .unwrap()
                .contains("Your default worker root")
        );
    }

    /// Workers discover a workspace's project skills, the orchestrator does
    /// not, so the workspace list names them. A skill living in an umbrella
    /// root is attributed to that umbrella, not repeated under nested roots.
    #[test]
    fn telegram_workspaces_block_lists_project_skills_once_under_their_root() {
        let _home = crate::test_support::temp_zdx_home();

        let dir = temp_dir();
        let umbrella = dir.path().to_path_buf();
        let project = umbrella.join("dub");
        std::fs::create_dir_all(umbrella.join(".zdx/skills/parity-flow")).unwrap();
        std::fs::write(
            umbrella.join(".zdx/skills/parity-flow/SKILL.md"),
            "---\nname: parity-flow\ndescription: ExampleCo release flow.\n---\nbody\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.join(".zdx/skills/dub-attest")).unwrap();
        std::fs::write(
            project.join(".zdx/skills/dub-attest/SKILL.md"),
            "---\nname: dub-attest\ndescription: Run device attestation end to end.\n---\nbody\n",
        )
        .unwrap();

        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    (
                        "dub".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1001,
                            cwd: project.display().to_string(),
                            orchestrator: false,
                            worker_root: None,
                        },
                    ),
                    (
                        "parity".to_string(),
                        TelegramProfileConfig {
                            chat_id: -1002,
                            cwd: umbrella.display().to_string(),
                            orchestrator: false,
                            worker_root: None,
                        },
                    ),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let skills = workspace_skills(&config);
        assert_eq!(
            skills.get("dub").unwrap(),
            &vec![(
                "dub-attest".to_string(),
                "Run device attestation end to end.".to_string()
            )]
        );
        assert_eq!(
            skills.get("parity").unwrap(),
            &vec![(
                "parity-flow".to_string(),
                "ExampleCo release flow.".to_string()
            )]
        );

        let block = telegram_workspaces_block(&config, None).unwrap();
        assert!(block.contains("  - skill `dub-attest`: Run device attestation end to end."));
        assert_eq!(block.matches("skill `parity-flow`").count(), 1);
    }

    /// Two workspace groups may share one root. Neither silently claims the
    /// root's skills: both list them.
    #[test]
    fn workspaces_sharing_a_root_both_list_its_skills() {
        let _home = crate::test_support::temp_zdx_home();

        let dir = temp_dir();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".zdx/skills/shared-flow")).unwrap();
        std::fs::write(
            root.join(".zdx/skills/shared-flow/SKILL.md"),
            "---\nname: shared-flow\ndescription: Shared flow.\n---\nbody\n",
        )
        .unwrap();
        let workspace = |chat_id: i64| TelegramProfileConfig {
            chat_id,
            cwd: root.display().to_string(),
            orchestrator: false,
            worker_root: None,
        };
        let config = Config {
            telegram: TelegramConfig {
                profiles: BTreeMap::from([
                    ("alpha".to_string(), workspace(-1001)),
                    ("beta".to_string(), workspace(-1002)),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };

        let expected = vec![("shared-flow".to_string(), "Shared flow.".to_string())];
        let skills = workspace_skills(&config);
        assert_eq!(skills.get("alpha"), Some(&expected));
        assert_eq!(skills.get("beta"), Some(&expected));

        let block = telegram_workspaces_block(&config, None).unwrap();
        assert_eq!(block.matches("skill `shared-flow`").count(), 2);
    }

    #[test]
    fn orchestrator_overlay_loads_only_when_present_and_non_empty() {
        let dir = temp_dir();
        let path = dir.path().join("orchestrator.md");

        // Missing file is a silent no-op.
        assert_eq!(load_orchestrator_overlay(&path), None);

        // Whitespace-only content is treated as absent.
        std::fs::write(&path, "  \n\t\n").unwrap();
        assert_eq!(load_orchestrator_overlay(&path), None);

        std::fs::write(&path, "\n- Reply in English\n- dub first\n").unwrap();
        assert_eq!(
            load_orchestrator_overlay(&path).as_deref(),
            Some("- Reply in English\n- dub first")
        );
    }
}
