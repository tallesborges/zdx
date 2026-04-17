//! Subagent delegation tool.
//!
//! Allows the model to delegate a scoped task to an isolated child `zdx exec`
//! run and return response text only.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, ToolSet, toolset_tool_names};
use crate::core::context::{PromptContextInclusion, build_prompt_with_context_and_layers};
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::providers::{ProviderKind, resolve_provider};
use crate::subagents::{self, RuntimeSubagentSelection, SubagentSummary};

/// Returns the tool definition for the `invoke_subagent` tool.
pub fn definition() -> ToolDefinition {
    definition_with_subagents(&[])
}

/// Returns the tool definition enriched with available named subagents.
pub fn definition_with_subagents(subagents: &[SubagentSummary]) -> ToolDefinition {
    let valid_subagents = supported_subagent_names(subagents);
    ToolDefinition {
        name: "Invoke_Subagent".to_string(),
        description: build_description(subagents),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Self-contained instructions for the delegated subagent. Include the goal, relevant context, constraints/non-goals, file paths, expected output, and verification when useful. The child does not share your full parent reasoning/history, so do not rely on implicit context."
                },
                "subagent": {
                    "type": "string",
                    "description": build_subagent_field_description(subagents),
                    "enum": valid_subagents
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct SubagentInput {
    prompt: String,
    subagent: Option<String>,
}

/// Executes the `invoke_subagent` tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    if !ctx.subagents_enabled {
        return ToolOutput::failure(
            "disabled",
            "invoke_subagent is disabled in config",
            Some("Set [subagents].enabled = true to enable subagent delegation".to_string()),
        );
    }

    let (input, prompt) = match parse_input(input) {
        Ok(value) => value,
        Err(err) => return err,
    };
    let config = ctx.config.clone().unwrap_or_default();
    let selection = match resolve_subagent_selection(&ctx.root, input.subagent.clone()) {
        Ok(selection) => selection,
        Err(err) => return err,
    };

    let definition = match &selection {
        RuntimeSubagentSelection::Default => None,
        RuntimeSubagentSelection::Named(definition) => Some(definition),
    };

    let model = match resolve_execution_model(definition, &config, ctx) {
        Ok(model) => model,
        Err(err) => return err,
    };
    let prompt = build_delegated_prompt(
        &prompt,
        child_has_read_thread_access(definition, &config, &model),
        ctx.current_thread_id.as_deref(),
    );

    let system_prompt = match build_system_prompt(&config, &ctx.root, definition, &model) {
        Ok(prompt) => prompt,
        Err(err) => return err,
    };

    let options = build_exec_options(definition, ctx, model, system_prompt);

    match run_exec_subagent(&ctx.root, &prompt, &options).await {
        Ok(response) => ToolOutput::success(Value::String(response)),
        Err(err) => ToolOutput::failure(
            "execution_failed",
            "Subagent execution failed",
            Some(err.to_string()),
        ),
    }
}

fn parse_input(input: &Value) -> Result<(SubagentInput, String), ToolOutput> {
    let input: SubagentInput = serde_json::from_value(input.clone()).map_err(|e| {
        ToolOutput::failure(
            "invalid_input",
            "Invalid input for invoke_subagent tool",
            Some(format!("Parse error: {e}")),
        )
    })?;

    let prompt = input.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "prompt cannot be empty",
            None,
        ));
    }

    Ok((input, prompt))
}

fn build_delegated_prompt(
    prompt: &str,
    can_consult_origin_thread: bool,
    origin_thread_id: Option<&str>,
) -> String {
    if !can_consult_origin_thread {
        return prompt.to_string();
    }

    match origin_thread_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(thread_id) => format!(
            "This task was delegated from thread {thread_id}. If important context is missing or ambiguous, consult that thread before making assumptions.\n\n{prompt}"
        ),
        None => prompt.to_string(),
    }
}

fn child_has_read_thread_access(
    definition: Option<&subagents::SubagentDefinition>,
    config: &crate::config::Config,
    model: &str,
) -> bool {
    effective_child_tool_names(definition, config, model)
        .into_iter()
        .any(|tool| tool.eq_ignore_ascii_case("read_thread"))
}

fn effective_child_tool_names(
    definition: Option<&subagents::SubagentDefinition>,
    config: &crate::config::Config,
    model: &str,
) -> Vec<String> {
    if let Some(tools) = definition.and_then(|definition| definition.tools.clone()) {
        return tools;
    }

    let provider = resolve_provider(model).kind;
    let provider_config = config.providers.get(provider);
    if provider_config.tools.is_some() {
        let all_tool_names = crate::tools::all_tool_names();
        let all_tool_names_refs: Vec<&str> = all_tool_names
            .iter()
            .map(std::string::String::as_str)
            .collect();
        return provider_config
            .filter_tools(&all_tool_names_refs)
            .into_iter()
            .map(str::to_string)
            .collect();
    }

    let tool_set = if matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICodex) {
        ToolSet::OpenAICodex
    } else {
        ToolSet::Default
    };
    toolset_tool_names(tool_set)
}

fn resolve_subagent_selection(
    root: &std::path::Path,
    requested: Option<String>,
) -> Result<RuntimeSubagentSelection, ToolOutput> {
    let requested = normalize_optional(requested);
    subagents::resolve_runtime_selection(root, requested.as_deref()).map_err(|err| {
        let label = requested.unwrap_or_else(|| "<default>".to_string());
        ToolOutput::failure(
            "invalid_input",
            format!("Unknown subagent '{label}'"),
            Some(err.to_string()),
        )
    })
}

fn resolve_execution_model(
    definition: Option<&subagents::SubagentDefinition>,
    config: &crate::config::Config,
    ctx: &ToolContext,
) -> Result<String, ToolOutput> {
    let model = match definition.and_then(|definition| definition.model.clone()) {
        Some(model) => {
            if let Some(err) = validate_model_supported(&model, ctx) {
                return Err(err);
            }
            model
        }
        None => normalize_optional(ctx.model.clone()).unwrap_or_else(|| config.model.clone()),
    };

    if model.trim().is_empty() {
        return Err(ToolOutput::failure(
            "invalid_config",
            "No model available for invoke_subagent",
            Some("Ensure a parent/default model is available in config".to_string()),
        ));
    }

    Ok(model)
}

fn build_system_prompt(
    config: &crate::config::Config,
    root: &std::path::Path,
    definition: Option<&subagents::SubagentDefinition>,
    model: &str,
) -> Result<String, ToolOutput> {
    let result = match definition {
        Some(definition) => subagents::render_prompt(
            config,
            root,
            definition,
            model,
            PromptContextInclusion::default(),
        )
        .map_err(|err| {
            ToolOutput::failure(
                "invalid_input",
                format!("Failed to render subagent '{}'", definition.name),
                Some(err.to_string()),
            )
        }),
        None => build_prompt_with_context_and_layers(
            config,
            root,
            model,
            &[],
            false,
            PromptContextInclusion::default(),
        )
        .map(|effective| effective.prompt.unwrap_or_default())
        .map_err(|err| {
            ToolOutput::failure(
                "invalid_input",
                "Failed to build default subagent prompt",
                Some(err.to_string()),
            )
        }),
    }?;

    Ok(result)
}

fn build_exec_options(
    definition: Option<&subagents::SubagentDefinition>,
    ctx: &ToolContext,
    model: String,
    system_prompt: String,
) -> ExecSubagentOptions {
    ExecSubagentOptions {
        model: Some(model),
        system_prompt: Some(system_prompt),
        thinking_level: definition
            .and_then(|definition| definition.thinking_level)
            .or(ctx.thinking_level),
        no_tools: false,
        no_system_prompt: false,
        tools_override: definition.and_then(|definition| definition.tools.clone()),
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: ctx.timeout,
    }
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn supported_subagent_names(subagents: &[SubagentSummary]) -> Vec<String> {
    let mut names = Vec::with_capacity(subagents.len() + 1);
    names.push(subagents::TASK_BUILTIN_ALIAS_NAME.to_string());
    for subagent in subagents {
        if !names.iter().any(|name| name == &subagent.name) {
            names.push(subagent.name.clone());
        }
    }
    names
}

fn build_subagent_field_description(subagents: &[SubagentSummary]) -> String {
    let valid = supported_subagent_names(subagents)
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Optional named subagent or reserved runtime alias. Choose the most specialized available subagent when one clearly fits. Use `task` for the default delegated ZDX behavior with the base prompt + context when no named specialist fits and delegation is still worthwhile. Valid values: {valid}. Skill names are invalid unless they are also listed here."
    )
}

fn build_description(subagents: &[SubagentSummary]) -> String {
    let mut description = "Delegate a scoped task to an isolated child agent run. Best for complex multi-step work, output-heavy subtasks, or independent parallel implementation slices that would clutter the main context. Prefer doing the work directly when it is small enough to complete without delegation. Child runs are self-contained and do not share your full parent reasoning or implicit context, so every important decision, relevant detail, file path, constraint, non-goal, and acceptance criterion must be made explicit in the prompt. Provide a focused prompt with the goal, relevant context, constraints/non-goals, file paths, expected output, and how success should be verified. NEVER delegate trivial reads, searches, or edits you can do directly. Use `subagent` to select a named configuration, or `task` for the default delegated ZDX behavior when no named specialist fits. Returns response text only. Skill names are invalid unless they are also listed as supported subagents.".to_string();

    if !subagents.is_empty() {
        let listed = subagents
            .iter()
            .map(|subagent| format!("{} ({})", subagent.name, subagent.description))
            .collect::<Vec<_>>()
            .join(", ");
        description.push_str(" Available named subagents: ");
        description.push_str(&listed);
        description.push('.');
    }

    description
}

fn validate_model_supported(model: &str, ctx: &ToolContext) -> Option<ToolOutput> {
    let requested = canonical_model_id(model);
    let available: Vec<String> = ctx
        .subagent_available_models
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    let canonical_available: Vec<String> = available
        .iter()
        .map(|value| canonical_model_id(value))
        .collect();

    let allowed = canonical_available
        .iter()
        .any(|available| available.eq_ignore_ascii_case(&requested));
    if allowed {
        return None;
    }

    let details = if available.is_empty() {
        "No available subagent models. Enable at least one provider with models in config."
            .to_string()
    } else {
        format!("Available models: {}", available.join(", "))
    };

    Some(ToolOutput::failure(
        "model_not_supported",
        format!("Model '{model}' is not supported for invoke_subagent"),
        Some(details),
    ))
}

fn canonical_model_id(model: &str) -> String {
    let selection = resolve_provider(model);
    format!("{}:{}", selection.kind.id(), selection.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_delegated_prompt_without_thread_id_returns_original_prompt() {
        assert_eq!(
            build_delegated_prompt("diagnose this", true, None),
            "diagnose this"
        );
    }

    #[test]
    fn test_build_delegated_prompt_without_read_thread_access_returns_original_prompt() {
        assert_eq!(
            build_delegated_prompt("diagnose this", false, Some("thread-123")),
            "diagnose this"
        );
    }

    #[test]
    fn test_build_delegated_prompt_with_thread_id_prefixes_prompt() {
        let prompt = build_delegated_prompt("diagnose this", true, Some("thread-123"));
        assert!(prompt.starts_with(
            "This task was delegated from thread thread-123. If important context is missing or ambiguous, consult that thread before making assumptions."
        ));
        assert!(prompt.ends_with("\n\ndiagnose this"));
    }

    #[test]
    fn test_child_has_read_thread_access_for_explicit_subagent_tools() {
        let definition = subagents::SubagentDefinition {
            name: "oracle".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("oracle.md"),
            source: subagents::SubagentSource::BuiltIn,
            model: None,
            thinking_level: None,
            tools: Some(vec!["read".to_string(), "read_thread".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            prompt_body: "body".to_string(),
        };

        assert!(child_has_read_thread_access(
            Some(&definition),
            &crate::config::Config::default(),
            "anthropic:claude-sonnet-4-5"
        ));
    }

    #[test]
    fn test_child_has_read_thread_access_for_explicit_subagent_tools_without_read_thread() {
        let definition = subagents::SubagentDefinition {
            name: "designer".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("designer.md"),
            source: subagents::SubagentSource::BuiltIn,
            model: None,
            thinking_level: None,
            tools: Some(vec!["read".to_string(), "glob".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            prompt_body: "body".to_string(),
        };

        assert!(!child_has_read_thread_access(
            Some(&definition),
            &crate::config::Config::default(),
            "anthropic:claude-sonnet-4-5"
        ));
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Invoke_Subagent");
        assert!(def.description.contains("Use `subagent`"));
        assert!(def.description.contains("Skill names are invalid"));

        let required = def
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
        assert!(!required.iter().any(|v| v == "model"));
        assert!(!required.iter().any(|v| v == "subagent"));

        let subagent = def
            .input_schema
            .get("properties")
            .and_then(|props| props.get("subagent"))
            .unwrap();
        let enum_values = subagent
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(enum_values.iter().any(|v| v == "task"));
    }

    #[test]
    fn test_definition_has_no_system_prompt() {
        let def = definition();
        let props = def.input_schema.get("properties").unwrap();
        assert!(props.get("system_prompt").is_none());
        assert!(props.get("subagent").is_some());
        assert!(props.get("model").is_none());
    }

    #[test]
    fn test_input_validation_missing_prompt() {
        let input = json!({"subagent": "coder"});
        let parsed: Result<SubagentInput, _> = serde_json::from_value(input);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_resolve_model_priority() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());

        assert_eq!(
            normalize_optional(ctx.model.clone()).as_deref(),
            Some("openai:gpt-5.2")
        );
    }

    #[test]
    fn test_parent_model_does_not_require_supported_list_match() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());
        ctx.subagent_available_models = vec!["codex:gpt-5.3-codex".to_string()];

        let chosen = normalize_optional(ctx.model.clone());
        assert_eq!(chosen.as_deref(), Some("openai:gpt-5.2"));
    }

    #[test]
    fn test_validate_model_supported() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["codex:gpt-5.3-codex".to_string()];

        assert!(validate_model_supported("codex:gpt-5.3-codex", &ctx).is_none());
        assert!(validate_model_supported("openai:gpt-5.2", &ctx).is_some());
    }

    #[test]
    fn test_validate_model_supported_accepts_provider_alias_equivalence() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["openai-codex:gpt-5.3-codex".to_string()];

        assert!(validate_model_supported("codex:gpt-5.3-codex", &ctx).is_none());
    }

    #[test]
    fn test_validate_model_supported_fails_when_available_models_empty() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        assert!(validate_model_supported("openai:gpt-5.2", &ctx).is_some());
    }

    #[test]
    fn test_build_description_includes_available_subagents() {
        let desc = build_description(&[
            SubagentSummary {
                name: "coder".to_string(),
                description: "Coding helper".to_string(),
            },
            SubagentSummary {
                name: "researcher".to_string(),
                description: "Research helper".to_string(),
            },
        ]);

        assert!(desc.contains("coder (Coding helper)"));
        assert!(desc.contains("researcher (Research helper)"));
        assert!(desc.contains("`task`"));
        assert!(desc.contains("Child runs are self-contained"));
        assert!(desc.contains("do not share your full parent reasoning"));
        assert!(desc.contains("acceptance criterion"));
        assert!(desc.contains("Skill names are invalid"));
    }

    #[test]
    fn test_subagent_field_description_lists_valid_values() {
        let description = build_subagent_field_description(&[SubagentSummary {
            name: "oracle".to_string(),
            description: "Deep reasoning".to_string(),
        }]);

        assert!(description.contains("Valid values: `task`, `oracle`"));
        assert!(description.contains("most specialized available subagent"));
        assert!(description.contains("Skill names are invalid"));
    }

    #[test]
    fn test_task_alias_resolves_to_default_runtime_behavior() {
        let selection =
            resolve_subagent_selection(std::path::Path::new("."), Some("task".to_string()))
                .unwrap();

        assert_eq!(selection, RuntimeSubagentSelection::Default);
    }
}
