//! One-shot helper operations a chat surface needs: generated text (titles,
//! recaps, handoff seeds, drafted prompts), the `/btw` seed, and the start
//! of a seeded topic. Each takes the chat's root: its config gives the
//! helper models, and the thread's stored root (else the chat's) is where a
//! helper runs, as in the bot.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::protocol::ErrorCode;
use super::thread_ops::{coded, validate_thread_id};
use crate::config::Config;
use crate::core::thread_persistence;
use crate::core::topic_start::TopicStart;

/// `{op:"generate"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Generate {
    /// A topic title from the first message (`title_model`). Runs in the
    /// chat's root; `thread_id` names the thread it is for (lineage only).
    Title {
        root: PathBuf,
        text: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
    /// A recap of a thread (`tldr_model`).
    Tldr { root: PathBuf, thread_id: String },
    /// A `/handoff` seed: source header, the user's goal, generated
    /// background (`handoff_model`).
    Handoff {
        root: PathBuf,
        thread_id: String,
        #[serde(default)]
        goal: String,
    },
    /// A prompt drafted from a short intent (`prompt_builder_model`).
    PromptBuilder {
        root: PathBuf,
        text: String,
        #[serde(default)]
        thread_id: Option<String>,
    },
}

/// `{op:"topic_start"}`: the start of a topic seeded from `source_thread_id`
/// in the chat whose root is `root`, as `profile`; with `pick`, that pick
/// resolved against the start config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicStartRequest {
    pub root: PathBuf,
    pub source_thread_id: String,
    /// `orchestrator`, or absent for an ordinary agent.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub pick: Option<String>,
}

/// `{reply:"topic_start"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicStartReply {
    /// The model selection to create the thread with; `None`: none (the
    /// topic resolves its role's default every turn).
    pub selection: Option<String>,
    /// The label a picker shows.
    pub label: String,
}

fn load_config(root: &Path) -> Result<Config> {
    Config::load_layered(&crate::config::paths::config_layer_paths_for(root)).map_err(|err| {
        coded(
            ErrorCode::Invalid,
            format!("This chat's workspace config does not load: {err:#}"),
        )
    })
}

/// Where a helper for `thread_id` runs: its stored root, else the chat's.
fn run_root(thread_id: Option<&str>, chat_root: &Path) -> Result<PathBuf> {
    Ok(thread_id
        .map(thread_persistence::read_thread_root_path)
        .transpose()?
        .flatten()
        .map_or_else(|| chat_root.to_path_buf(), PathBuf::from))
}

fn existing_thread(thread_id: &str) -> Result<()> {
    validate_thread_id(thread_id)?;
    if !thread_persistence::thread_exists(thread_id) {
        return Err(coded(
            ErrorCode::NotFound,
            format!("thread '{thread_id}' does not exist"),
        ));
    }
    Ok(())
}

/// Runs one generation; returns the text.
///
/// # Errors
/// `invalid` for empty input or a config that does not load, `not_found`
/// for a missing thread; else the helper's failure (no code).
pub async fn generate(request: Generate) -> Result<String> {
    match request {
        Generate::Title {
            root,
            text,
            thread_id,
        } => {
            if text.trim().is_empty() {
                return Err(coded(ErrorCode::Invalid, "nothing to title"));
            }
            let config = load_config(&root)?;
            crate::core::title_generation::generate_title(
                &text,
                &config.title_model,
                &root,
                thread_id.as_deref(),
            )
            .await
        }
        Generate::Tldr { root, thread_id } => {
            existing_thread(&thread_id)?;
            let config = load_config(&root)?;
            let run_root = run_root(Some(&thread_id), &root)?;
            crate::core::tldr_generation::generate_tldr(&thread_id, &config.tldr_model, &run_root)
                .await
        }
        Generate::Handoff {
            root,
            thread_id,
            goal,
        } => {
            existing_thread(&thread_id)?;
            let config = load_config(&root)?;
            let run_root = run_root(Some(&thread_id), &root)?;
            crate::core::handoff_generation::generate_handoff(
                &thread_id,
                &goal,
                &config.handoff_model,
                &run_root,
                None,
            )
            .await
        }
        Generate::PromptBuilder {
            root,
            text,
            thread_id,
        } => {
            if text.trim().is_empty() {
                return Err(coded(
                    ErrorCode::Invalid,
                    "Prompt-builder intent cannot be empty",
                ));
            }
            if let Some(thread_id) = thread_id.as_deref() {
                validate_thread_id(thread_id)?;
            }
            let config = load_config(&root)?;
            let run_root = run_root(thread_id.as_deref(), &root)?;
            crate::core::prompt_builder_generation::generate_prompt_builder(
                &text,
                Some(config.prompt_builder_model.clone()),
                &run_root,
                None,
                thread_id,
            )
            .await
        }
    }
}

/// The `/btw` seed: a header pointing at the source thread and its
/// ancestors, then the question verbatim. No model call.
///
/// # Errors
/// `invalid` for an unusable thread id.
pub fn btw_seed(thread_id: &str, question: &str) -> Result<String> {
    validate_thread_id(thread_id)?;
    Ok(crate::core::handoff_generation::build_side_thread_seed(
        thread_id, question,
    ))
}

/// The start of a seeded topic, or a pick resolved against it.
///
/// # Errors
/// `invalid` when the chat's config does not load, the profile is not
/// `orchestrator`, the start does not resolve and nothing is picked (no
/// topic may be created), or the pick names an undefined mode.
pub fn topic_start(request: &TopicStartRequest) -> Result<TopicStartReply> {
    validate_thread_id(&request.source_thread_id)?;
    if let Some(profile) = request.profile.as_deref()
        && profile != crate::subagents::ORCHESTRATOR_SUBAGENT_NAME
    {
        return Err(coded(
            ErrorCode::Invalid,
            format!("a seeded topic starts as an agent or `orchestrator`, not `{profile}`"),
        ));
    }
    let config = load_config(&request.root).context("topic start")?;
    let start = TopicStart::for_source(
        config,
        &request.source_thread_id,
        request.profile.as_deref(),
    );
    if let Some(pick) = request.pick.as_deref() {
        let selection = start
            .resolve_pick(pick)
            .map_err(|err| coded(ErrorCode::Invalid, err.to_string()))?;
        let mut config = start.config.clone();
        config
            .apply_model_selection(&selection)
            .map_err(|err| coded(ErrorCode::Invalid, err.to_string()))?;
        return Ok(TopicStartReply {
            selection: Some(selection),
            label: config.model_label(),
        });
    }
    let selection = start
        .pinned_selection()
        .map_err(|refusal| coded(ErrorCode::Invalid, refusal))?;
    Ok(TopicStartReply {
        selection,
        label: start.label(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btw_seed_heads_the_question_with_its_source() {
        let _home = crate::test_support::temp_zdx_home();
        let seed = btw_seed("source-thread", "what changed?").unwrap();
        assert!(
            seed.starts_with("[Side question from thread source-thread"),
            "{seed}"
        );
        assert!(seed.ends_with("what changed?"), "{seed}");
        assert!(btw_seed("../x", "q").is_err());
    }

    #[test]
    fn topic_start_refuses_an_unresolved_start_but_takes_a_pick() {
        let home = crate::test_support::temp_zdx_home();
        let root = home.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let mut source = thread_persistence::Thread::with_id("source".to_string()).unwrap();
        source
            .set_model_override(Some("mode:gone".to_string()))
            .unwrap();
        let request = |pick: Option<&str>, profile: Option<&str>| TopicStartRequest {
            root: root.clone(),
            source_thread_id: "source".to_string(),
            profile: profile.map(str::to_string),
            pick: pick.map(str::to_string),
        };
        let err = topic_start(&request(None, None)).unwrap_err();
        assert_eq!(
            super::super::thread_ops::error_code(&err),
            Some(ErrorCode::Invalid)
        );
        assert!(format!("{err:#}").contains("`mode:gone`"), "{err:#}");
        let picked = topic_start(&request(Some("openai:gpt-5@high"), None)).unwrap();
        assert_eq!(picked.selection.as_deref(), Some("openai:gpt-5@high"));
        assert!(topic_start(&request(None, Some("explorer"))).is_err());

        let unpinned = thread_persistence::Thread::with_id("plain".to_string()).unwrap();
        drop(unpinned);
        let start = topic_start(&TopicStartRequest {
            source_thread_id: "plain".to_string(),
            ..request(None, None)
        })
        .unwrap();
        assert_eq!(start.selection, None, "an unpinned source pins nothing");
        assert!(!start.label.is_empty());
    }
}
