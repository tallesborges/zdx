//! The model a seeded topic (`/handoff`, `/btw`) starts with when nothing is
//! picked, and how a pick for it resolves. One rule for every surface, so a
//! picker never shows a different model than the topic is created with.
//!
//! A source that pinned its model (`/model`) hands on its own effective
//! model, resolved in the source's role. An unpinned source hands on nothing:
//! the new topic resolves the default of its *own* role on every turn.
//!
//! Fail closed: when the start does not resolve (the source's selection names
//! a mode the destination config lacks, or the orchestrator override names an
//! undefined mode), no topic may be created. Running the default instead
//! could use another provider or account.

use crate::config::{Config, ThinkingLevel, UnknownModeError};
use crate::core::thread_persistence;
use crate::subagents::{ORCHESTRATOR_SUBAGENT_NAME, apply_orchestrator_override};

/// The start of a seeded topic, before any pick.
#[derive(Debug, Clone)]
pub struct TopicStart {
    /// The config the new topic starts with; a pick resolves against it.
    pub config: Config,
    /// The source pinned a model or thinking level.
    source_pinned: bool,
    /// The source's selection when it names a mode `config` lacks.
    unresolved: Option<String>,
    /// Why the orchestrator role's model does not resolve.
    profile_error: Option<String>,
}

impl TopicStart {
    /// The start of a topic seeded from `source_thread_id` as `profile`
    /// (`None`: an ordinary agent), with the destination chat's `config`.
    #[must_use]
    pub fn for_source(config: Config, source_thread_id: &str, profile: Option<&str>) -> Self {
        let source_profile = thread_persistence::read_persistent_profile(source_thread_id)
            .ok()
            .flatten();
        let model = thread_persistence::read_thread_model_override(source_thread_id)
            .ok()
            .flatten();
        let thinking = thread_persistence::read_thread_thinking_override(source_thread_id)
            .ok()
            .flatten();
        Self::layer(
            config,
            is_orchestrator(source_profile.as_deref()),
            is_orchestrator(profile),
            model.as_deref(),
            thinking,
        )
    }

    /// [`Self::for_source`] from the source's stored values.
    #[must_use]
    pub fn layer(
        mut config: Config,
        source_orchestrator: bool,
        new_orchestrator: bool,
        model_override: Option<&str>,
        thinking_override: Option<ThinkingLevel>,
    ) -> Self {
        let source_pinned = model_override.is_some() || thinking_override.is_some();
        let orchestrator = if source_pinned {
            source_orchestrator
        } else {
            new_orchestrator
        };
        let profile_error = orchestrator
            .then(|| apply_orchestrator_override(&mut config).err())
            .flatten()
            .map(|err| format!("{err:#}"));
        let unresolved = config
            .apply_thread_model_override(model_override, thinking_override)
            .err()
            .map(|err| err.selection);
        Self {
            config,
            source_pinned,
            unresolved,
            profile_error,
        }
    }

    /// Why no topic may be created from this start, if so.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if let Some(err) = &self.profile_error {
            return Some(err.clone());
        }
        self.unresolved.as_ref().map(|selection| {
            format!(
                "`{selection}` names a model mode this chat's config does not define; pick a model for the new topic"
            )
        })
    }

    /// The selection the new topic is created with when nothing is picked:
    /// `None` when the source pinned nothing (the topic follows its role's
    /// default).
    ///
    /// # Errors
    /// [`Self::refusal`], when the start does not resolve.
    pub fn pinned_selection(&self) -> Result<Option<String>, String> {
        if let Some(refusal) = self.refusal() {
            return Err(refusal);
        }
        Ok(self.source_pinned.then(|| self.config.model_selection()))
    }

    /// The label a picker shows for this start.
    #[must_use]
    pub fn label(&self) -> String {
        match self.refusal() {
            Some(refusal) => format!("⚠️ {refusal}"),
            None => self.config.model_label(),
        }
    }

    /// A pick for the new topic, resolved against its start config: the
    /// selection to keep.
    ///
    /// # Errors
    /// The pick names a mode this config does not define.
    pub fn resolve_pick(&self, pick: &str) -> Result<String, UnknownModeError> {
        let mut config = self.config.clone();
        config.apply_model_selection(pick)?;
        Ok(config.model_selection())
    }
}

fn is_orchestrator(profile: Option<&str>) -> bool {
    profile == Some(ORCHESTRATOR_SUBAGENT_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SubagentOverride;

    #[test]
    fn follows_the_new_role_unless_the_source_pinned_one() {
        let mut base = Config {
            model: "openai:agent-model@high".to_string(),
            thinking_level: ThinkingLevel::High,
            ..Default::default()
        };
        base.subagents.overrides.insert(
            ORCHESTRATOR_SUBAGENT_NAME.to_string(),
            SubagentOverride {
                model: Some("openai:orchestrator-model@low".to_string()),
                thinking_level: Some(ThinkingLevel::Low),
            },
        );
        let start = |source, new, model: Option<&str>, thinking| {
            TopicStart::layer(base.clone(), source, new, model, thinking)
        };

        let to_orchestrator = start(false, true, None, None);
        assert_eq!(
            to_orchestrator.config.model,
            "openai:orchestrator-model@low"
        );
        assert_eq!(to_orchestrator.pinned_selection().unwrap(), None);

        let to_agent = start(true, false, None, None);
        assert_eq!(to_agent.config.model, "openai:agent-model@high");

        let pinned = start(false, true, Some("openai:pinned-model@medium"), None);
        assert_eq!(pinned.config.model, "openai:pinned-model@medium");
        assert!(pinned.pinned_selection().unwrap().is_some());

        let thinking_only = start(true, false, None, Some(ThinkingLevel::Medium));
        assert_eq!(
            thinking_only.config.model,
            "openai:orchestrator-model@medium"
        );
    }

    /// Deliberate change from the earlier bot rule, which created the topic
    /// pinned to the unresolved mode (its turns then refused): no topic is
    /// created at all, and the picker says why.
    #[test]
    fn an_unresolved_start_refuses_creation() {
        let base = Config {
            model: "openai:default-model@low".to_string(),
            thinking_level: ThinkingLevel::Low,
            ..Config::default()
        };
        let start = TopicStart::layer(base, false, false, Some("mode:gone"), None);
        let err = start.pinned_selection().unwrap_err();
        assert!(err.contains("`mode:gone`"), "{err}");
        assert!(start.label().starts_with("⚠️"), "{}", start.label());
        // A concrete pick still resolves, so the user can open the topic.
        assert_eq!(
            start.resolve_pick("openai:other@high").unwrap(),
            "openai:other@high"
        );
        assert!(start.resolve_pick("mode:gone").is_err());

        let mut base = Config::default();
        base.subagents.overrides.insert(
            ORCHESTRATOR_SUBAGENT_NAME.to_string(),
            SubagentOverride {
                model: Some("mode:gone".to_string()),
                thinking_level: None,
            },
        );
        let start = TopicStart::layer(base, true, false, None, Some(ThinkingLevel::High));
        let err = start.pinned_selection().unwrap_err();
        assert!(err.contains("`mode:gone`"), "{err}");
    }
}
